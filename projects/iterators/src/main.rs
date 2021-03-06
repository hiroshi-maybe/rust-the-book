fn main() {
    {
        let v1 = vec![1, 2, 3];
        let it = v1.iter();
        for val in it {
            // This does not work because it is moved in `for` loop
            // it.next();
            println!("Got: {}", val);
        }
        // This does not work because it is moved in `for` loop
        // it.next();
    }

    {
        let v1: Vec<i32> = vec![1, 2, 3];
        let v2 = v1.iter().map(|&x| x + 1).collect::<Vec<_>>();
        //let v2: Vec<_> = v1.iter().map(|x| x + 1).collect();
        assert_eq!(v2, vec![2, 3, 4]);
    }
}

#[test]
fn iterator_demo() {
    let v1 = vec![1, 2, 3];
    let mut it = v1.iter();

    assert_eq!(it.next(), Some(&1));
    assert_eq!(it.next(), Some(&2));
    assert_eq!(it.next(), Some(&3));
    assert_eq!(it.next(), None);
}

#[test]
fn iterator_sum() {
    let v1 = vec![1, 2, 3];
    let it = v1.iter();
    let tot: i32 = it.sum();

    assert_eq!(tot, 6);

    // This does not work because it is moved in the `sum()` call
    // it.next();
}
